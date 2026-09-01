//! Archive safety over dirty, ignored, and hook-created content.

use super::code::*;

use std::time::Duration;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{CodeSessionId, CodeSessionLifecycle, CodeWorkspaceStatus, WorkspaceId};
use tidebreak_harness::HarnessEvent;

#[tokio::test]
async fn archive_requires_force_when_the_tree_is_dirty() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let path = workspace["worktree_path"].as_str().unwrap();
    std::fs::write(std::path::Path::new(path).join("dirty.txt"), "nope\n").unwrap();

    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);

    let forced = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(forced.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = forced.json().await.unwrap();
    assert_eq!(body["status"], "released");
    assert!(!std::path::Path::new(path).exists());
}

#[tokio::test]
async fn force_archive_skips_ignored_content_inspection() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let path = workspace["worktree_path"].as_str().unwrap();

    assert!(std::process::Command::new("git")
        .args([
            "config",
            "--add",
            "tidebreak.archiveDisposablePath",
            "../outside-worktree",
        ])
        .current_dir(path)
        .status()
        .unwrap()
        .success());

    let forced = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();

    assert_eq!(forced.status(), reqwest::StatusCode::OK);
    assert!(!std::path::Path::new(path).exists());
}

#[tokio::test]
async fn no_force_archive_of_a_dirty_workspace_leaves_an_idle_session() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
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
    let path = workspace["worktree_path"].as_str().unwrap();
    std::fs::write(std::path::Path::new(path).join("dirty.txt"), "nope\n").unwrap();

    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "uncommitted");

    let parsed: CodeSessionId = json_id(&session).parse().unwrap();
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.lifecycle, CodeSessionLifecycle::Idle);
    assert!(std::path::Path::new(path).exists());

    let turn = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "still here" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        turn.status(),
        reqwest::StatusCode::ACCEPTED,
        "session must stay usable after a refused dirty archive: {}",
        turn.text().await.unwrap()
    );
}

/// Decision 0032: the archive script obeys the same failure-preserves rule as
/// setup. A script whose job is to back the workspace up must be able to stop
/// the archive by failing, and a refused archive must not have run it at all.
#[tokio::test]
async fn a_failing_archive_script_preserves_the_worktree() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "path": repo,
            "archive_script": "echo ran >> .archive-ran; exit 4",
        }))
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
            "title": "backed up on archive",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let workspace: serde_json::Value = created.json().await.unwrap();
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    std::fs::write(path.join("dirty.txt"), "nope\n").unwrap();

    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    assert!(
        !path.join(".archive-ran").exists(),
        "a refused archive must not run the archive script"
    );

    let failed = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = failed.json().await.unwrap();
    assert_eq!(body["kind"], "archive_script_failed");
    assert!(path.join(".archive-ran").is_file());
    assert!(
        path.join("dirty.txt").is_file(),
        "a failed archive script must leave the worktree on disk"
    );
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
    assert_eq!(listed[0]["status"], "active");
}

#[tokio::test]
async fn archive_preserves_hook_created_files_without_force() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "path": repo,
            "archive_script": "echo kept > hook-created.txt",
        }))
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
            "title": "hook output",
        }))
        .send()
        .await
        .unwrap();
    let workspace: serde_json::Value = created.json().await.unwrap();
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    let archived = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = archived.json().await.unwrap();
    assert_eq!(body["kind"], "uncommitted");
    assert!(path.join("hook-created.txt").is_file());
    let stored = tidebreak_core::db::code::get_workspace(
        &_runtime.db,
        &tidebreak_core::OwnerId::local(),
        json_id(&workspace).parse().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.status, CodeWorkspaceStatus::Active);
}

#[cfg(unix)]
#[tokio::test]
async fn archive_rejects_concurrent_writes_after_the_first_check() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let started = dir.path().join("archive-started");
    let proceed = dir.path().join("archive-proceed");
    let script = format!(
        "touch \"{}\"; while [ ! -f \"{}\" ]; do sleep 0.01; done",
        started.display(),
        proceed.display()
    );
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "path": repo,
            "archive_script": script,
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
            "title": "late writer",
        }))
        .send()
        .await
        .unwrap();
    let workspace: serde_json::Value = created.json().await.unwrap();
    let workspace_id = json_id(&workspace).to_owned();
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    let archive_client = client.clone();
    let archive_token = token.clone();
    let archive = tokio::spawn(async move {
        archive_client
            .post(format!(
                "http://{addr}/code/workspaces/{workspace_id}/archive"
            ))
            .bearer_auth(archive_token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap()
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !started.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "archive hook did not reach its barrier"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let terminal = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/terminals",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(terminal.status(), reqwest::StatusCode::CONFLICT);
    let terminal_body: serde_json::Value = terminal.json().await.unwrap();
    assert_eq!(terminal_body["kind"], "workspace_not_ready");

    std::fs::write(path.join("concurrent.txt"), "late\n").unwrap();
    std::fs::write(&proceed, "go\n").unwrap();
    let archived = archive.await.unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = archived.json().await.unwrap();
    assert_eq!(body["kind"], "uncommitted");
    assert_eq!(
        std::fs::read_to_string(path.join("concurrent.txt")).unwrap(),
        "late\n"
    );
}

#[tokio::test]
async fn archive_shutdown_timeout_preserves_the_checkout() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    runtime.set_archive_shutdown_timeout(true);
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    let archived = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = archived.json().await.unwrap();
    assert_eq!(body["kind"], "workspace_shutdown_timeout");
    assert!(path.join("README.md").is_file());
    let stored = tidebreak_core::db::code::get_workspace(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        json_id(&workspace).parse().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.status, CodeWorkspaceStatus::Archiving);
}

#[tokio::test]
async fn archive_recovery_finishes_after_checkout_removal() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let owner = tidebreak_core::OwnerId::local();
    let (_registered, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id: tidebreak_core::WorkspaceId = json_id(&workspace).parse().unwrap();
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    assert!(tidebreak_core::db::code::compare_and_set_workspace_status(
        &runtime.db,
        &owner,
        workspace_id,
        CodeWorkspaceStatus::Active,
        CodeWorkspaceStatus::Archiving,
    )
    .await
    .unwrap());
    let removed = std::process::Command::new("git")
        .current_dir(&repo)
        .args(["worktree", "remove", "--force"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(removed.success());
    assert!(!path.exists());

    let restarted = CodeRuntime::with_registry(
        runtime.db.clone(),
        dir.path().to_path_buf(),
        scripted_registry(),
    );
    restarted.recover().await.unwrap();

    let stored = tidebreak_core::db::code::get_workspace(&runtime.db, &owner, workspace_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, CodeWorkspaceStatus::Released);
    assert!(stored.archived_at.is_some());
    assert!(stored.released_at.is_some());
}

#[tokio::test]
async fn archive_refuses_ignored_only_content_without_force() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    std::fs::write(repo.join(".gitignore"), ".env.local\nbuild/\n").unwrap();
    for args in [
        ["add", ".gitignore"].as_slice(),
        ["commit", "-m", "ignore local files"].as_slice(),
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
    }
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    std::fs::write(path.join(".env.local"), "ONLY_COPY=1\n").unwrap();

    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "ignored_content");
    assert_eq!(
        std::fs::read_to_string(path.join(".env.local")).unwrap(),
        "ONLY_COPY=1\n"
    );

    assert!(std::process::Command::new("git")
        .args([
            "config",
            "--add",
            "tidebreak.archiveDisposablePath",
            "build"
        ])
        .current_dir(&path)
        .status()
        .unwrap()
        .success());
    std::fs::remove_file(path.join(".env.local")).unwrap();
    std::fs::create_dir_all(path.join("build")).unwrap();
    std::fs::write(path.join("build/cache.bin"), "generated\n").unwrap();
    let archived = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);
    assert!(!path.exists());
}

#[tokio::test]
async fn archive_ends_the_session_before_removing_the_worktree() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
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
    let path = workspace["worktree_path"].as_str().unwrap().to_owned();
    let archived = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);
    assert!(!std::path::Path::new(&path).exists());
    let parsed: CodeSessionId = json_id(&session).parse().unwrap();
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.lifecycle, CodeSessionLifecycle::Ended);

    let again = client
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
    assert_eq!(
        again.status(),
        reqwest::StatusCode::CONFLICT,
        "archived workspace is not ready for a new session"
    );
}

#[tokio::test]
async fn archive_refuses_a_running_session_without_force() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta {
                text: "working".into(),
            },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(50)),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
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
    let session_id = json_id(&session).to_owned();
    let parsed: CodeSessionId = session_id.parse().unwrap();
    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "busy" }))
                .send()
                .await
                .unwrap()
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let row = tidebreak_core::db::code::get_session(
                &runtime.db,
                &tidebreak_core::OwnerId::local(),
                parsed,
            )
            .await
            .unwrap()
            .unwrap();
            if row.lifecycle == CodeSessionLifecycle::Running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("turn never reached Running");

    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "session_running");

    let forced = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(forced.status(), reqwest::StatusCode::OK);
    let _ = turn.await;
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.lifecycle, CodeSessionLifecycle::Ended);
}

/// A freshly created workspace has a branch with no commits past its base.
/// Archive must still succeed, because git cannot bundle an empty revision
/// range.
#[tokio::test]
async fn archive_succeeds_for_a_branch_with_no_commits_past_its_base() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace);
    let branch = workspace["branch_name"].as_str().unwrap().to_owned();
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    let archived = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/archive"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);
    let archived_body: serde_json::Value = archived.json().await.unwrap();
    assert_eq!(archived_body["status"], "released");
    assert!(archived_body["bundle_bytes"].is_null());
    assert!(archived_body["released_tip"].as_str().is_some());
    assert!(!path.exists(), "archive removes the checkout");
    assert!(
        !branch_exists_in(&repo, &branch),
        "archive drops the base-only branch"
    );

    let restored = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/restore"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(restored.status(), reqwest::StatusCode::OK);
    let restored_body: serde_json::Value = restored.json().await.unwrap();
    assert_eq!(restored_body["status"], "active");
    assert!(path.join("README.md").is_file());
    assert!(
        branch_exists_in(&repo, &branch),
        "restore recreates the base-only branch"
    );
}

/// A failed release write leaves the branch and bundle available for retry.
#[tokio::test]
async fn archive_persists_release_metadata_before_returning_success() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id: WorkspaceId = json_id(&workspace).parse().unwrap();
    let branch = workspace["branch_name"].as_str().unwrap().to_owned();
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    std::fs::write(path.join("saved.txt"), "saved work\n").unwrap();
    for args in [
        ["add", "saved.txt"].as_slice(),
        ["commit", "-m", "saved work"].as_slice(),
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
    }

    runtime.fail_next_workspace_release_metadata();
    let failed = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/archive"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::CONFLICT);
    let after_failure = client
        .get(format!("http://{addr}/code/workspaces/{workspace_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let after_failure: serde_json::Value = after_failure.json().await.unwrap();
    assert_eq!(after_failure["status"], "archiving");
    assert!(after_failure["released_tip"].is_null());
    assert!(
        !path.exists(),
        "archive removes the checkout before release"
    );
    assert!(
        branch_exists_in(&repo, &branch),
        "a failed release write keeps the branch"
    );
    let bundle = runtime
        .data_dir
        .join("code")
        .join("bundles")
        .join(format!("{}.bundle", workspace_id.as_uuid()));
    assert!(bundle.is_file(), "a failed release write keeps the bundle");

    let retried = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/archive"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(retried.status(), reqwest::StatusCode::OK);
    let retried_body: serde_json::Value = retried.json().await.unwrap();
    assert_eq!(retried_body["status"], "released");
    assert!(retried_body["released_tip"].as_str().is_some());
    assert!(
        !branch_exists_in(&repo, &branch),
        "the successful retry drops the branch"
    );

    let restored = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/restore"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        restored.status(),
        reqwest::StatusCode::OK,
        "restore failed: {}",
        restored.text().await.unwrap()
    );
    let restored_body: serde_json::Value = restored.json().await.unwrap();
    assert_eq!(restored_body["status"], "active");
    assert!(path.join("saved.txt").is_file());
}
