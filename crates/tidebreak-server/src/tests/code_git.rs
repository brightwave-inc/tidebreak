//! Hosted delivery routes against a fake forge.

use super::*;

use std::net::Ipv4Addr;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;

use crate::code::CodeRuntime;
use crate::obo_gateway::test_support::FakeLender;
use crate::obo_gateway::GitCredentialLender;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_harness::AdapterRegistry;

/// The git app, optionally on a "hosted machine" that lends gateway git
/// credentials (decision 63).
async fn code_app_with(
    lender: Option<Arc<dyn GitCredentialLender>>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("code-git.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    let mut runtime = CodeRuntime::with_registry(db, dir.path().to_path_buf(), registry);
    if let Some(lender) = lender {
        runtime = runtime.with_git_credentials(lender);
    }
    let runtime = Arc::new(runtime);
    let config = Config::desktop(dir.path());
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
    let token = state.token.clone();
    (app(state), token, runtime, dir)
}

async fn serve(router: Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

fn init_paired_repo(dir: &std::path::Path) -> std::path::PathBuf {
    let bare = dir.join("origin.git");
    let work = dir.join("work");
    run(dir, &["git", "init", "--bare", bare.to_str().unwrap()]);
    std::fs::create_dir_all(&work).unwrap();
    run(&work, &["git", "init", "-b", "main"]);
    run(&work, &["git", "config", "user.email", "dev@example.com"]);
    run(&work, &["git", "config", "user.name", "Dev"]);
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    run(&work, &["git", "add", "README.md"]);
    run(&work, &["git", "commit", "-m", "init"]);
    run(
        &work,
        &["git", "remote", "add", "origin", bare.to_str().unwrap()],
    );
    run(&work, &["git", "push", "-u", "origin", "main"]);
    work
}

fn run(cwd: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new(args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()
        .unwrap();
    assert!(status.success(), "{args:?} failed in {}", cwd.display());
}

async fn error_kind(response: reqwest::Response) -> (reqwest::StatusCode, String, String) {
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    (
        status,
        body["kind"].as_str().unwrap_or_default().to_owned(),
        body["message"].as_str().unwrap_or_default().to_owned(),
    )
}

fn assert_borrowed_forge_credential(headers: &axum::http::HeaderMap) {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert_eq!(bearer, "Bearer ghs_fake_borrowed");
}

async fn register_delivery_repository(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    root: &std::path::Path,
) {
    let response = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "path": root }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
}

/// Issues #2673 and #2700: a gateway-authenticated hosted caller reads and
/// acts through one borrowed credential per repository operation. Every
/// request stays pinned to the registered host and repository.
#[tokio::test]
async fn a_hosted_delivery_page_reads_and_acts_over_forge_rest() {
    type RecordedActions = Arc<std::sync::Mutex<Vec<serde_json::Value>>>;
    let recorded_actions: RecordedActions = Arc::default();

    fn pull_request() -> serde_json::Value {
        serde_json::json!({
            "number": 17,
            "html_url": "https://github.com/acme/demo/pull/17",
            "title": "Repair hosted delivery",
            "body": "Read the delivery detail over REST.",
            "state": "open",
            "draft": false,
            "user": {
                "login": "mira-chen",
                "avatar_url": "https://avatars.example/mira"
            },
            "head": {
                "ref": "hosted-delivery",
                "sha": "feedfeedfeedfeedfeed",
                "repo": {
                    "name": "demo",
                    "full_name": "acme/demo",
                    "owner": { "login": "acme" }
                }
            },
            "base": { "ref": "main" },
            "labels": [{ "name": "code" }],
            "assignees": [{ "login": "mira-chen" }],
            "requested_reviewers": [{ "login": "reviewer" }],
            "comments": 2,
            "changed_files": 1,
            "additions": 12,
            "deletions": 3,
            "commits": 2,
            "created_at": "2026-08-25T10:00:00Z",
            "updated_at": "2026-08-25T11:00:00Z",
            "merged_at": null,
            "closed_at": null,
            "node_id": "PR_kwDOTEST17"
        })
    }

    let repository = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "name": "demo",
            "full_name": "acme/demo",
            "html_url": "https://github.com/acme/demo",
            "default_branch": "main",
            "owner": { "login": "acme" },
        }))
    };
    let pulls = |headers: axum::http::HeaderMap,
                 axum::extract::Query(query): axum::extract::Query<
        std::collections::HashMap<String, String>,
    >| async move {
        assert_borrowed_forge_credential(&headers);
        assert_eq!(query.get("state").map(String::as_str), Some("open"));
        // GitHub's pull list omits `comments`; the issues list carries the
        // integer count the overlay reads.
        let mut listed = pull_request();
        listed
            .as_object_mut()
            .expect("pull request fixture is an object")
            .remove("comments");
        axum::Json(serde_json::json!([listed]))
    };
    let issues = |headers: axum::http::HeaderMap,
                  axum::extract::Query(query): axum::extract::Query<
        std::collections::HashMap<String, String>,
    >| async move {
        assert_borrowed_forge_credential(&headers);
        assert_eq!(query.get("state").map(String::as_str), Some("open"));
        axum::Json(serde_json::json!([{
            "number": 17,
            "comments": 2,
            "pull_request": {
                "url": "https://github.com/acme/demo/pull/17"
            }
        }]))
    };
    let pull = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        let mut pull = pull_request();
        pull["draft"] = serde_json::Value::Bool(true);
        axum::Json(pull)
    };
    let issue_comments = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([{
            "id": 100,
            "body": "Please keep the hosted path scoped.",
            "user": { "login": "reviewer" },
            "created_at": "2026-08-25T11:05:00Z"
        }]))
    };
    let reviews = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([]))
    };
    let inline_comments = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([]))
    };
    let files = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([{
            "filename": "crates/tidebreak-server/src/code/delivery.rs",
            "status": "modified",
            "additions": 12,
            "deletions": 3,
            "patch": "@@ -1 +1 @@"
        }]))
    };
    let checks = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "check_runs": [{
                "name": "desktop test",
                "status": "completed",
                "conclusion": "failure",
                "details_url": "https://github.com/acme/demo/actions/runs/44"
            }, {
                "name": "clippy",
                "status": "in_progress",
                "conclusion": null,
                "html_url": "https://github.com/acme/demo/actions/runs/45"
            }]
        }))
    };
    let workflow_runs = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "workflow_runs": [{
                "id": 44,
                "run_attempt": 2,
                "status": "completed",
                "conclusion": "failure",
                "display_title": "Desktop CI",
                "name": "CI",
                "html_url": "https://github.com/acme/demo/actions/runs/44",
                "head_branch": "hosted-delivery",
                "head_sha": "feedfeedfeedfeedfeed",
                "event": "pull_request",
                "actor": { "login": "mira-chen" },
                "created_at": "2026-08-25T10:00:00Z",
                "updated_at": "2026-08-25T11:00:00Z"
            }]
        }))
    };
    let workflow_run = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "id": 44,
            "run_attempt": 2,
            "status": "completed",
            "conclusion": "failure",
            "display_title": "Desktop CI",
            "name": "CI",
            "html_url": "https://github.com/acme/demo/actions/runs/44",
            "head_branch": "hosted-delivery",
            "head_sha": "feedfeedfeedfeedfeed",
            "event": "pull_request",
            "actor": { "login": "mira-chen" },
            "created_at": "2026-08-25T10:00:00Z",
            "updated_at": "2026-08-25T11:00:00Z"
        }))
    };
    let jobs = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "jobs": [{
                "id": 501,
                "name": "test",
                "status": "completed",
                "conclusion": "failure",
                "html_url": "https://github.com/acme/demo/actions/runs/44/job/501",
                "steps": [{ "name": "Run tests", "conclusion": "failure" }]
            }]
        }))
    };
    let deployments = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([{
            "id": 91,
            "environment": "production",
            "ref": "hosted-delivery",
            "sha": "feedfeedfeedfeedfeed",
            "creator": { "login": "mira-chen" },
            "created_at": "2026-08-25T10:30:00Z",
            "updated_at": "2026-08-25T10:30:00Z"
        }]))
    };
    let deployment = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "id": 91,
            "environment": "production",
            "ref": "hosted-delivery",
            "sha": "feedfeedfeedfeedfeed",
            "creator": { "login": "mira-chen" },
            "created_at": "2026-08-25T10:30:00Z",
            "updated_at": "2026-08-25T10:30:00Z"
        }))
    };
    let deployment_statuses = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([{
            "id": 92,
            "state": "success",
            "description": "Deployed",
            "environment_url": "https://demo.example",
            "log_url": "https://github.com/acme/demo/deployments/91",
            "created_at": "2026-08-25T10:35:00Z"
        }]))
    };
    let timeline = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([
            { "event": "committed" },
            { "event": "added_to_merge_queue" }
        ]))
    };
    let graphql_actions = Arc::clone(&recorded_actions);
    let graphql = move |headers: axum::http::HeaderMap,
                        axum::Json(body): axum::Json<serde_json::Value>| {
        let recorded = Arc::clone(&graphql_actions);
        async move {
            assert_borrowed_forge_credential(&headers);
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(serde_json::json!({ "action": "graphql", "body": body }));
            axum::Json(serde_json::json!({
                "data": {
                    "enablePullRequestAutoMerge": {
                        "pullRequest": { "number": 17 }
                    }
                }
            }))
        }
    };
    let merge_actions = Arc::clone(&recorded_actions);
    let merge = move |headers: axum::http::HeaderMap,
                      axum::Json(body): axum::Json<serde_json::Value>| {
        let recorded = Arc::clone(&merge_actions);
        async move {
            assert_borrowed_forge_credential(&headers);
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(serde_json::json!({ "action": "merge", "body": body }));
            axum::Json(serde_json::json!({
                "merged": true,
                "message": "Pull Request successfully merged"
            }))
        }
    };
    let state_actions = Arc::clone(&recorded_actions);
    let update_pull = move |headers: axum::http::HeaderMap,
                            axum::Json(body): axum::Json<serde_json::Value>| {
        let recorded = Arc::clone(&state_actions);
        async move {
            assert_borrowed_forge_credential(&headers);
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(serde_json::json!({ "action": "state", "body": body }));
            axum::Json(pull_request())
        }
    };
    let comment_actions = Arc::clone(&recorded_actions);
    let comment = move |headers: axum::http::HeaderMap,
                        axum::Json(body): axum::Json<serde_json::Value>| {
        let recorded = Arc::clone(&comment_actions);
        async move {
            assert_borrowed_forge_credential(&headers);
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(serde_json::json!({ "action": "comment", "body": body }));
            (
                axum::http::StatusCode::CREATED,
                axum::Json(serde_json::json!({ "id": 101 })),
            )
        }
    };
    let rerun_actions = Arc::clone(&recorded_actions);
    let rerun = move |headers: axum::http::HeaderMap,
                      axum::extract::Path(run_id): axum::extract::Path<u64>| {
        let recorded = Arc::clone(&rerun_actions);
        async move {
            assert_borrowed_forge_credential(&headers);
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(serde_json::json!({ "action": "rerun", "run_id": run_id }));
            axum::http::StatusCode::CREATED
        }
    };
    let rerun_failed_actions = Arc::clone(&recorded_actions);
    let rerun_failed =
        move |headers: axum::http::HeaderMap,
              axum::extract::Path(run_id): axum::extract::Path<u64>| {
            let recorded = Arc::clone(&rerun_failed_actions);
            async move {
                assert_borrowed_forge_credential(&headers);
                recorded
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(serde_json::json!({
                        "action": "rerun_failed",
                        "run_id": run_id
                    }));
                axum::http::StatusCode::CREATED
            }
        };
    let forge = axum::Router::new()
        .route("/repos/acme/demo", axum::routing::get(repository))
        .route("/repos/acme/demo/pulls", axum::routing::get(pulls))
        .route("/repos/acme/demo/issues", axum::routing::get(issues))
        .route(
            "/repos/acme/demo/pulls/17",
            axum::routing::get(pull).patch(update_pull),
        )
        .route("/repos/acme/demo/pulls/17/merge", axum::routing::put(merge))
        .route("/graphql", axum::routing::post(graphql))
        .route(
            "/repos/acme/demo/issues/17/timeline",
            axum::routing::get(timeline),
        )
        .route(
            "/repos/acme/demo/issues/17/comments",
            axum::routing::get(issue_comments).post(comment),
        )
        .route(
            "/repos/acme/demo/pulls/17/reviews",
            axum::routing::get(reviews),
        )
        .route(
            "/repos/acme/demo/pulls/17/comments",
            axum::routing::get(inline_comments),
        )
        .route("/repos/acme/demo/pulls/17/files", axum::routing::get(files))
        .route(
            "/repos/acme/demo/commits/{sha}/check-runs",
            axum::routing::get(checks),
        )
        .route(
            "/repos/acme/demo/actions/runs",
            axum::routing::get(workflow_runs),
        )
        .route(
            "/repos/acme/demo/actions/runs/44",
            axum::routing::get(workflow_run),
        )
        .route(
            "/repos/acme/demo/actions/runs/44/jobs",
            axum::routing::get(jobs),
        )
        .route(
            "/repos/acme/demo/actions/runs/{run_id}/rerun",
            axum::routing::post(rerun),
        )
        .route(
            "/repos/acme/demo/actions/runs/{run_id}/rerun-failed-jobs",
            axum::routing::post(rerun_failed),
        )
        .route(
            "/repos/acme/demo/deployments",
            axum::routing::get(deployments),
        )
        .route(
            "/repos/acme/demo/deployments/91",
            axum::routing::get(deployment),
        )
        .route(
            "/repos/acme/demo/deployments/91/statuses",
            axum::routing::get(deployment_statuses),
        );
    let forge_addr = serve(forge).await;

    let lender = Arc::new(FakeLender::offering_person("mira-chen"));
    let (router, token, runtime, dir) =
        code_app_with(Some(lender.clone() as Arc<dyn GitCredentialLender>)).await;
    runtime.set_gh_search_path(Some("/path/with/no/gh".into()));
    runtime.set_forge_api_base(Some(format!("http://{forge_addr}")));
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let root = init_paired_repo(dir.path());
    run(
        &root,
        &[
            "git",
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/demo.git",
        ],
    );
    register_delivery_repository(&client, addr, &token, &root).await;

    let repositories: serde_json::Value = client
        .get(format!("http://{addr}/code/delivery/repositories"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(repositories["capability"]["authenticated"], true);
    assert_eq!(repositories["capability"]["viewer_login"], "mira-chen");
    assert_eq!(
        repositories["repositories"][0]["name_with_owner"],
        "acme/demo"
    );
    assert_eq!(repositories["repositories"][0]["default_branch"], "main");

    let target = serde_json::json!({
        "host": "github.com",
        "owner": "acme",
        "name": "demo"
    });
    let pull_requests: serde_json::Value = client
        .post(format!("http://{addr}/code/delivery/pull-requests/query"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repositories": [target.clone()],
            "states": ["open"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pull_requests["items"][0]["number"], 17);
    assert_eq!(pull_requests["items"][0]["in_merge_queue"], true);
    assert_eq!(pull_requests["items"][0]["comment_count"], 2);
    assert_eq!(
        pull_requests["items"][0]["checks"][0]["name"],
        "desktop test"
    );
    assert_eq!(pull_requests["items"][0]["checks"][0]["bucket"], "fail");
    assert_eq!(
        pull_requests["items"][0]["checks"][0]["workflow_run_id"],
        44
    );
    assert_eq!(pull_requests["items"][0]["checks"][1]["bucket"], "pending");
    assert_eq!(
        pull_requests["items"][0]["checks"][1]["workflow_run_id"],
        45
    );
    assert!(
        pull_requests["items"][0]["attention_reasons"]
            .as_array()
            .unwrap()
            .is_empty(),
        "a queued pull request leaves the attention list"
    );

    let pull_request_detail: serde_json::Value = client
        .post(format!("http://{addr}/code/delivery/pull-requests/detail"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repository": target.clone(),
            "number": 17
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pull_request_detail["summary"]["number"], 17);
    assert_eq!(pull_request_detail["summary"]["in_merge_queue"], true);
    assert_eq!(
        pull_request_detail["can_mark_ready"], false,
        "hosted REST details must not advertise the unsupported ready transition"
    );
    assert_eq!(
        pull_request_detail["body"],
        "Read the delivery detail over REST."
    );
    assert_eq!(pull_request_detail["comments"][0]["author"], "reviewer");
    assert_eq!(
        pull_request_detail["files"][0]["path"],
        "crates/tidebreak-server/src/code/delivery.rs"
    );

    let runs: serde_json::Value = client
        .post(format!("http://{addr}/code/delivery/runs/query"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "repositories": [target.clone()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runs["items"].as_array().unwrap().len(), 2, "{runs}");
    assert!(runs["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["kind"] == "workflow_run"));
    assert!(runs["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["kind"] == "deployment"));

    let workflow_detail: serde_json::Value = client
        .post(format!("http://{addr}/code/delivery/runs/detail"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repository": target.clone(),
            "kind": "workflow_run",
            "id": 44
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(workflow_detail["summary"]["github_id"], 44);
    assert_eq!(workflow_detail["jobs"][0]["name"], "test");
    assert_eq!(workflow_detail["jobs"][0]["failed_steps"][0], "Run tests");

    let deployment_detail: serde_json::Value = client
        .post(format!("http://{addr}/code/delivery/runs/detail"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repository": target,
            "kind": "deployment",
            "id": 91
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(deployment_detail["summary"]["github_id"], 91);
    assert_eq!(
        deployment_detail["deployment_statuses"][0]["state"],
        "success"
    );

    for body in [
        serde_json::json!({
            "target": { "repository": target.clone(), "number": 17 },
            "action": {
                "type": "merge",
                "method": "squash",
                "auto": false,
                "admin": false,
                "expected_head_sha": "feedfeedfeedfeedfeed"
            }
        }),
        serde_json::json!({
            "target": { "repository": target.clone(), "number": 17 },
            "action": {
                "type": "merge",
                "method": "squash",
                "auto": true,
                "admin": false,
                "expected_head_sha": "feedfeedfeedfeedfeed"
            }
        }),
        serde_json::json!({
            "target": { "repository": target.clone(), "number": 17 },
            "action": { "type": "close" }
        }),
        serde_json::json!({
            "target": { "repository": target.clone(), "number": 17 },
            "action": { "type": "reopen" }
        }),
        serde_json::json!({
            "target": { "repository": target.clone(), "number": 17 },
            "action": { "type": "comment", "body": "  Ship this change.  " }
        }),
        serde_json::json!({
            "target": { "repository": target.clone(), "number": 17 },
            "action": {
                "type": "rerun_failed",
                "workflow_run_ids": [45, 44, 44]
            }
        }),
    ] {
        let response = client
            .post(format!("http://{addr}/code/delivery/pull-requests/action"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    for action in ["rerun", "rerun_failed"] {
        let response = client
            .post(format!("http://{addr}/code/delivery/runs/action"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "target": {
                    "repository": target.clone(),
                    "kind": "workflow_run",
                    "id": 44
                },
                "action": { "type": action }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    let borrows_before_unsupported = lender.minted().len();
    for (action, expected_kind) in [
        (
            serde_json::json!({ "type": "mark_ready" }),
            "git_forge_mark_ready_unsupported",
        ),
        (
            serde_json::json!({
                "type": "merge",
                "method": "squash",
                "auto": false,
                "admin": true,
                "expected_head_sha": "feedfeedfeedfeedfeed"
            }),
            "git_forge_admin_merge_unsupported",
        ),
    ] {
        let response = client
            .post(format!("http://{addr}/code/delivery/pull-requests/action"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "target": { "repository": target.clone(), "number": 17 },
                "action": action
            }))
            .send()
            .await
            .unwrap();
        let (status, kind, message) = error_kind(response).await;
        assert_eq!(status, reqwest::StatusCode::CONFLICT);
        assert_eq!(kind, expected_kind);
        assert!(
            message.contains("Open the pull request on GitHub"),
            "{message}"
        );
    }
    assert_eq!(
        lender.minted().len(),
        borrows_before_unsupported,
        "unsupported hosted actions do not borrow a credential"
    );

    let actions = recorded_actions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(actions.contains(&serde_json::json!({
        "action": "merge",
        "body": {
            "sha": "feedfeedfeedfeedfeed",
            "merge_method": "squash"
        }
    })));
    let auto_merge = actions
        .iter()
        .find(|action| action["action"] == "graphql")
        .expect("hosted auto-merge posts the pinned mutation");
    assert_eq!(
        auto_merge["body"]["variables"],
        serde_json::json!({
            "id": "PR_kwDOTEST17",
            "oid": "feedfeedfeedfeedfeed",
            "method": "SQUASH"
        })
    );
    assert!(
        auto_merge["body"]["query"]
            .as_str()
            .unwrap_or_default()
            .contains("enablePullRequestAutoMerge"),
        "{auto_merge}"
    );
    assert!(actions.contains(&serde_json::json!({
        "action": "state",
        "body": { "state": "closed" }
    })));
    assert!(actions.contains(&serde_json::json!({
        "action": "state",
        "body": { "state": "open" }
    })));
    assert!(actions.contains(&serde_json::json!({
        "action": "comment",
        "body": { "body": "Ship this change." }
    })));
    assert!(actions.contains(&serde_json::json!({
        "action": "rerun",
        "run_id": 44
    })));
    assert_eq!(
        actions
            .iter()
            .filter(|action| action["action"] == "rerun_failed")
            .count(),
        3,
        "two pull request runs and one run-detail action reach the failed-jobs endpoint"
    );
    assert_eq!(
        lender.minted(),
        vec!["acme/demo".to_owned(); 15],
        "every read and action borrows only for the registered repository"
    );
}
