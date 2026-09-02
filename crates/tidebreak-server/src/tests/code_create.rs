//! Session and workspace creation, listing, and setup recovery.

use super::code::*;
use super::*;

use std::sync::Arc;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{RepoId, Store, WorkspaceId};
use tidebreak_harness::AdapterRegistry;

#[tokio::test]
async fn listing_workspace_sessions_returns_create_shaped_snapshots() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let missing = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            uuid::Uuid::new_v4()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let empty = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(empty.status(), reqwest::StatusCode::OK);
    let empty_body: Vec<serde_json::Value> = empty.json().await.unwrap();
    assert!(empty_body.is_empty());

    let created = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], created["id"]);
    assert_eq!(listed[0]["workspace_id"], created["workspace_id"]);
    assert_eq!(listed[0]["harness_kind"], "claude_code");
    assert_eq!(listed[0]["permission_mode"], "plan");
    assert_eq!(listed[0]["lifecycle"], created["lifecycle"]);
}

#[tokio::test]
async fn listing_session_turns_returns_user_input_and_usage() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let missing = client
        .get(format!(
            "http://{addr}/code/sessions/{}/turns",
            uuid::Uuid::new_v4()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let empty = client
        .get(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(empty.status(), reqwest::StatusCode::OK);
    let empty_body: Vec<serde_json::Value> = empty.json().await.unwrap();
    assert!(empty_body.is_empty());

    let first = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "hello" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let second = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "again" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let listed = client
        .get(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0]["id"], first["id"]);
    assert_eq!(listed[0]["ordinal"], 1);
    assert_eq!(listed[0]["status"], "completed");
    assert_eq!(listed[0]["user_input"], "hello");
    assert_eq!(listed[0]["usage"]["output_tokens"], 6);
    assert!(listed[0]["started_at"].is_string());
    assert!(listed[0]["ended_at"].is_string());
    assert_eq!(listed[1]["id"], second["id"]);
    assert_eq!(listed[1]["ordinal"], 2);
    assert_eq!(listed[1]["user_input"], "again");
}

#[tokio::test]
async fn remote_create_routes_write_remote_rows_without_a_host_checkout() {
    let (router, token, runtime, dir) =
        code_app_with_remote(ScriptedAdapter::new(plain_text_script()), None).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo_named(dir.path(), "remote-create");
    add_github_remote(&repo, "remote-create");
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let registered: serde_json::Value = registered.json().await.unwrap();
    record_github_origin(&runtime, &registered, "remote-create").await;

    let workspace_url = format!("http://{addr}/code/remote/workspaces");
    let unauthenticated = client
        .post(&workspace_url)
        .json(&serde_json::json!({
            "repo_id": json_id(&registered),
            "title": "remote route",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let created = client
        .post(&workspace_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&registered),
            "title": "remote route",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let workspace: serde_json::Value = created.json().await.unwrap();
    let workspace_id: WorkspaceId = json_id(&workspace).parse().unwrap();
    assert_eq!(workspace["title"], "remote route");
    assert_eq!(workspace["worktree_path"], format!("remote:{workspace_id}"));
    assert_eq!(workspace["base_ref"], "main");

    let stored = runtime
        .get_workspace(&tidebreak_core::OwnerId::local(), workspace_id)
        .await
        .unwrap();
    assert!(stored.is_remote());

    let session_url = format!("http://{addr}/code/remote/workspaces/{workspace_id}/sessions");
    let unauthenticated = client
        .post(&session_url)
        .json(&serde_json::json!({ "harness": "claude_code" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let created = client
        .post(&session_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "model": "openai::gpt-5.6-sol",
            "reasoning_effort": "high",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    assert_eq!(session["workspace_id"], workspace_id.to_string());
    assert_eq!(session["permission_mode"], "allow");
    assert_eq!(session["fast_mode"], false);
    assert_eq!(session["model"], "openai::gpt-5.6-sol");
    assert_eq!(session["reasoning_effort"], "high");
    assert_eq!(session["lifecycle"], "idle");

    let unsupported_mode = client
        .post(&session_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unsupported_mode.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = unsupported_mode.json().await.unwrap();
    assert_eq!(body["kind"], "bad_request", "{body}");
    assert!(body["message"]
        .as_str()
        .is_some_and(|message| message.contains("unknown field `permission_mode`")));
}

#[tokio::test]
async fn remote_create_routes_keep_configuration_and_workspace_refusals_typed() {
    let (router, token, _runtime, _dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let disabled = client
        .post(format!("http://{addr}/code/remote/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "repo_id": RepoId::new() }))
        .send()
        .await
        .unwrap();
    assert_eq!(disabled.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = disabled.json().await.unwrap();
    assert_eq!(body["kind"], "remote_disabled", "{body}");

    let (router, token, _runtime, dir) =
        code_app_with_remote(ScriptedAdapter::new(plain_text_script()), None).await;
    let addr = serve(router).await;
    let repo = init_git_repo_named(dir.path(), "local-create");
    add_github_remote(&repo, "local-create");
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let refused = client
        .post(format!(
            "http://{addr}/code/remote/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "harness": "claude_code" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "workspace_not_remote", "{body}");
}

#[tokio::test]
async fn workspace_setup_failure_preserves_the_checkout() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "path": repo,
            "setup_script": "exit 3",
        }))
        .send()
        .await
        .unwrap();
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "broken setup",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces?repo_id={}",
            json_id(&repo_body)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["status"], "setup_failed");
    let path = listed[0]["worktree_path"].as_str().unwrap();
    assert!(std::path::Path::new(path).join("README.md").is_file());
}

/// Quick actions are only reachable if a client can write them. `create`
/// stores the list, `patch` replaces it whole, and an empty list clears it.
#[tokio::test]
async fn quick_actions_round_trip_through_create_and_patch() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "path": repo,
            "quick_actions": [
                { "name": "  Test  ", "command": " cargo test ", "auto_run_on_create": false },
                { "name": "Install", "command": "pnpm install", "auto_run_on_create": true },
            ],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    let repo_id = json_id(&repo_body);
    assert_eq!(repo_body["quick_actions"][0]["name"], "Test");
    assert_eq!(repo_body["quick_actions"][0]["command"], "cargo test");
    assert_eq!(repo_body["quick_actions"][1]["auto_run_on_create"], true);

    let patched = client
        .patch(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "quick_actions": [
                { "name": "Lint", "command": "cargo clippy", "auto_run_on_create": false },
            ],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), reqwest::StatusCode::OK);
    let patched: serde_json::Value = patched.json().await.unwrap();
    assert_eq!(patched["quick_actions"].as_array().unwrap().len(), 1);
    assert_eq!(patched["quick_actions"][0]["name"], "Lint");

    // Two actions under one name would make the second unreachable, because
    // running one looks it up by name.
    let duplicate = client
        .patch(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "quick_actions": [
                { "name": "Lint", "command": "cargo clippy", "auto_run_on_create": false },
                { "name": "Lint", "command": "biome lint", "auto_run_on_create": false },
            ],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), reqwest::StatusCode::BAD_REQUEST);

    let blank = client
        .patch(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "quick_actions": [{ "name": "Lint", "command": "   ", "auto_run_on_create": false }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(blank.status(), reqwest::StatusCode::BAD_REQUEST);

    // Nothing the server allocates is sized from the body. Every entry also
    // renders in the header menu and the palette, so the list is bounded.
    let over_cap: Vec<serde_json::Value> = (0..33)
        .map(|index| {
            serde_json::json!({
                "name": format!("Action {index}"),
                "command": "true",
                "auto_run_on_create": false,
            })
        })
        .collect();
    let too_many = client
        .patch(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "quick_actions": over_cap }))
        .send()
        .await
        .unwrap();
    assert_eq!(too_many.status(), reqwest::StatusCode::BAD_REQUEST);
    let refusal: serde_json::Value = too_many.json().await.unwrap();
    // The refusal names the limit, so a client can fix the body without guessing.
    assert!(
        refusal["message"]
            .as_str()
            .is_some_and(|message| message.contains("32")),
        "refusal should name the cap, got {refusal}"
    );

    // Exactly the cap is still accepted.
    let at_cap: Vec<serde_json::Value> = (0..32)
        .map(|index| {
            serde_json::json!({
                "name": format!("Action {index}"),
                "command": "true",
                "auto_run_on_create": false,
            })
        })
        .collect();
    let accepted = client
        .patch(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "quick_actions": at_cap }))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::OK);
    assert_eq!(
        accepted.json::<serde_json::Value>().await.unwrap()["quick_actions"]
            .as_array()
            .unwrap()
            .len(),
        32
    );

    let long_name = client
        .patch(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "quick_actions": [{
                "name": "n".repeat(65),
                "command": "true",
                "auto_run_on_create": false,
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(long_name.status(), reqwest::StatusCode::BAD_REQUEST);

    let long_command = client
        .patch(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "quick_actions": [{
                "name": "Lint",
                "command": "x".repeat(1025),
                "auto_run_on_create": false,
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(long_command.status(), reqwest::StatusCode::BAD_REQUEST);

    // Back to one so the clear-vs-omit checks below read against a known list.
    let reset = client
        .patch(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "quick_actions": [
                { "name": "Lint", "command": "cargo clippy", "auto_run_on_create": false },
            ],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reset.status(), reqwest::StatusCode::OK);

    // Omitting the field leaves the stored list alone; an empty list clears it.
    let renamed = client
        .patch(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "display_name": "renamed" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(renamed["quick_actions"].as_array().unwrap().len(), 1);
    let cleared = client
        .patch(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "quick_actions": [] }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(cleared["quick_actions"].as_array().unwrap().is_empty());
}

/// A failed setup keeps the checkout, so the way out is to fix the script and
/// run it again — not to cut a second worktree. Retrying takes the workspace
/// Active on the same path.
#[tokio::test]
async fn retry_setup_revives_a_setup_failed_workspace_in_place() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let repo_body: serde_json::Value = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo, "setup_script": "exit 3" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let repo_id = json_id(&repo_body).to_owned();
    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "repo_id": repo_id, "title": "broken setup" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let listed = client
        .get(format!("http://{addr}/code/workspaces?repo_id={repo_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    let workspace_id = json_id(&listed[0]).to_owned();
    let worktree_path = listed[0]["worktree_path"].as_str().unwrap().to_owned();

    // Still broken: the retry fails the same way and the status holds.
    let again = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/retry-setup"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        again.json::<serde_json::Value>().await.unwrap()["kind"],
        "setup_failed"
    );

    let fixed = client
        .patch(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "setup_script": "echo ok > .setup-ran" }))
        .send()
        .await
        .unwrap();
    assert_eq!(fixed.status(), reqwest::StatusCode::OK);
    let revived = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/retry-setup"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(revived.status(), reqwest::StatusCode::OK);
    let revived: serde_json::Value = revived.json().await.unwrap();
    assert_eq!(revived["status"], "active");
    // The same worktree, not a second one.
    assert_eq!(revived["worktree_path"], worktree_path);
    assert!(std::path::Path::new(&worktree_path)
        .join(".setup-ran")
        .is_file());
    let listed = client
        .get(format!("http://{addr}/code/workspaces?repo_id={repo_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);

    // An already-active workspace is a no-op, not a re-run.
    let repeat = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/retry-setup"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(repeat.status(), reqwest::StatusCode::OK);
}

/// A definitively signed-out engine refuses at create with a message the UI
/// can show, instead of minting a session that dies on turn one with the
/// provider's raw 401 (#2653).
#[tokio::test]
async fn a_signed_out_engine_refuses_at_create_not_on_turn_one() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_authenticated(Some(false));
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;

    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "harness_not_authenticated", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("Claude Code is not signed in on this machine."),
        "{body}"
    );
}

/// Signing in after the doctor has cached a signed-out probe must not leave
/// create stuck on that answer. Create re-probes a cached refusal rather
/// than waiting for an explicit doctor refresh.
#[tokio::test]
async fn create_re_probes_a_cached_signed_out_observation() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_authenticated(Some(false));
    let (router, token, _runtime, dir) = code_app_with(adapter.clone()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;

    let report = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(report["harnesses"][0]["authenticated"], false);
    assert_eq!(adapter.probe_count(), 1);

    adapter.set_authenticated(Some(true));
    let created = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    assert!(
        adapter.probe_count() >= 2,
        "create must re-probe a cached signed-out observation, not reuse it"
    );
}

/// Only a definitive signed-out observation refuses. A probe that could not
/// verify the sign-in state answers `None`, and a false refusal on a working
/// machine is strictly worse than the first-turn failure it would prevent.
#[tokio::test]
async fn an_unverified_sign_in_state_does_not_block_create() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_authenticated(None);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
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
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
}

/// The #2742 guarantee holds at create time: on a gateway-hosted machine the
/// relay authenticates covered engines as the caller, so a signed-out engine
/// still creates a session freely.
#[tokio::test]
async fn a_signed_out_relay_covered_engine_still_creates_on_a_hosted_machine() {
    let (dir, store) = temp_db_store("code.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(
        ScriptedAdapter::new(plain_text_script()).with_authenticated(Some(false)),
    ));
    let gateway = Arc::new(
        crate::obo_gateway::OboGateway::new(
            "https://gateway.example",
            "tidebreak:feedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeed".to_owned(),
        )
        .unwrap(),
    );
    let runtime = Arc::new(
        CodeRuntime::with_registry(db, dir.path().to_path_buf(), registry).with_harness_llm(
            Arc::new(crate::code::harness_llm::HarnessLlmRelay::new(gateway)),
        ),
    );
    let mut state = AppState::new(
        Config::desktop(dir.path()),
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
    let token = state.token.clone();
    let addr = serve(app(state)).await;
    // Boot publishes the loopback base the relay wiring needs before any
    // worker attaches; tests serve the router directly, so publish it here.
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
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
}

fn init_git_repo_on_branch(
    dir: &std::path::Path,
    branch: &str,
    commit: bool,
) -> std::path::PathBuf {
    let repo = dir.join("origin");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        ["git", "init", "-b", branch].as_slice(),
        ["git", "config", "user.email", "dev@example.com"].as_slice(),
        ["git", "config", "user.name", "Dev"].as_slice(),
        ["git", "config", "core.autocrlf", "false"].as_slice(),
    ] {
        assert!(std::process::Command::new(args[0])
            .args(&args[1..])
            .current_dir(&repo)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .unwrap()
            .success());
    }
    if commit {
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        assert!(std::process::Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
    }
    repo
}

#[tokio::test]
async fn first_turn_starts_when_the_repository_has_no_main_ref() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo_on_branch(dir.path(), "trunk", true);

    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    assert_eq!(repo_body["default_base_ref"], "trunk");

    let patched = client
        .patch(format!("http://{addr}/code/repos/{}", json_id(&repo_body)))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "default_base_ref": "main" }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), reqwest::StatusCode::OK);

    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "first change",
            "base_ref": "main",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let workspace: serde_json::Value = created.json().await.unwrap();
    assert_eq!(workspace["base_ref"], "trunk");

    let persisted = client
        .get(format!("http://{addr}/code/repos/{}", json_id(&repo_body)))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(persisted["default_base_ref"], "trunk");

    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = session.json().await.unwrap();

    let turn = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);
    let turn: serde_json::Value = turn.json().await.unwrap();
    assert_eq!(turn["status"], "completed");
}

#[tokio::test]
async fn workspace_create_names_the_missing_base_ref_when_nothing_resolves() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo_on_branch(dir.path(), "trunk", false);

    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let repo_body: serde_json::Value = registered.json().await.unwrap();

    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "first change",
            "base_ref": "main",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = created.json().await.unwrap();
    assert_eq!(body["kind"], "missing_base_ref");
    let message = body["message"].as_str().unwrap_or("");
    assert!(
        message.contains("`main`"),
        "error must name the missing ref: {message}"
    );
    assert!(
        message.contains("trunk"),
        "error must name the candidates you tried: {message}"
    );
}

#[tokio::test]
async fn workspace_create_does_not_fall_through_an_explicit_missing_ref() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo_on_branch(dir.path(), "trunk", true);

    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    assert_eq!(repo_body["default_base_ref"], "trunk");

    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "first change",
            "base_ref": "feature-x",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = created.json().await.unwrap();
    assert_eq!(body["kind"], "missing_base_ref");
    let message = body["message"].as_str().unwrap_or("");
    assert!(
        message.contains("`feature-x`"),
        "error must name the missing ref: {message}"
    );
    assert!(
        !message.contains("trunk"),
        "an explicit miss must not walk sibling defaults: {message}"
    );
}
