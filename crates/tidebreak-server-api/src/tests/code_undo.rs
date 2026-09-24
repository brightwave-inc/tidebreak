//! Undo in the worktree through the routes: checkpoint restore and its undo,
//! file and hunk revert, discard, and the refusals while a turn runs.

use super::code::*;

use std::time::Duration;

use crate::code::bus::CodeLiveUpdate;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{CapLevel, Event, OwnerId, SessionId, WorkspaceId};

async fn create_session(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    workspace: &serde_json::Value,
    mode: &str,
) -> String {
    let created = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(workspace)
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({ "harness": "claude_code", "permission_mode": mode }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = created.json().await.unwrap();
    json_id(&body).to_owned()
}

/// Run one scripted turn to the end and return its row.
async fn run_turn(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    session: &str,
    message: &str,
) -> serde_json::Value {
    let turn = run_turn_to_end(
        client,
        addr,
        token,
        session,
        serde_json::json!({ "message": message }),
    )
    .await;
    assert_eq!(turn["status"], "completed", "{turn}");
    turn
}

async fn preview(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    workspace: &str,
    query: (&str, &str),
) -> reqwest::Response {
    client
        .get(format!(
            "http://{addr}/code/workspaces/{workspace}/checkpoints/restore"
        ))
        .bearer_auth(token)
        .query(&[query])
        .send()
        .await
        .unwrap()
}

async fn post(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    path: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    client
        .post(format!("http://{addr}{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

fn read(path: std::path::PathBuf) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[tokio::test]
async fn a_restore_goes_back_to_before_a_turn_journals_itself_and_can_be_undone() {
    let adapter = ScriptedAdapter::new(plain_text_script());
    let (router, token, runtime, dir) = code_app_with(adapter.clone()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace).to_owned();
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let session = create_session(&client, addr, &token, &workspace, "plan").await;

    // Turn 1 leaves a plan; turn 2 edits it and adds a module; then a person
    // edits by hand. The scripted engine writes nothing, so the edits land in
    // the worktree before each turn ends, where the checkpoint sees them.
    std::fs::write(worktree.join("plan.md"), "step one\n").unwrap();
    run_turn(&client, addr, &token, &session, "plan it").await;
    std::fs::write(worktree.join("plan.md"), "step one\nstep two\n").unwrap();
    std::fs::write(worktree.join("module.rs"), "pub fn two() {}\n").unwrap();
    let second = run_turn(&client, addr, &token, &session, "build it").await;
    std::fs::write(worktree.join("by-hand.txt"), "typed by a person\n").unwrap();

    let response = preview(
        &client,
        addr,
        &token,
        &workspace_id,
        ("turn", json_id(&second)),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let previewed: serde_json::Value = response.json().await.unwrap();
    let mut lost: Vec<&str> = previewed["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    lost.sort_unstable();
    assert_eq!(
        lost,
        ["by-hand.txt", "module.rs", "plan.md"],
        "the confirmation names changes made by hand as well as by the turn"
    );
    assert_eq!(previewed["session_id"], session.as_str());

    let mut updates = runtime.bus.subscribe_updates(&OwnerId::local());
    let response = post(
        &client,
        addr,
        &token,
        &format!("/code/workspaces/{workspace_id}/checkpoints/restore"),
        serde_json::json!({
            "target": { "kind": "before_turn", "turn_id": json_id(&second) },
            "expected_tree": previewed["current_tree"],
        }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let restored: serde_json::Value = response.json().await.unwrap();
    let restore_id = restored["restore_id"].as_str().unwrap().to_owned();

    assert_eq!(
        read(worktree.join("plan.md")).as_deref(),
        Some("step one\n")
    );
    assert!(!worktree.join("module.rs").exists());
    assert!(!worktree.join("by-hand.txt").exists());
    let workspace_uuid: WorkspaceId = workspace_id.parse().unwrap();
    let mut announced = false;
    while let Ok(update) = updates.try_recv() {
        announced |= update == CodeLiveUpdate::FilesChanged(workspace_uuid);
    }
    assert!(announced, "every view of the worktree re-reads it");
    let session_id: SessionId = session.parse().unwrap();
    let journaled = journaled_events(&runtime.db, session_id).await;
    let rows: Vec<Event> = journaled
        .iter()
        .map(|framed| framed.event.clone())
        .filter(|event| matches!(event, Event::CheckpointRestored { .. }))
        .collect();
    let [Event::CheckpointRestored {
        status: started, ..
    }, Event::CheckpointRestored {
        restore_id: journaled_id,
        target,
        diffstat,
        actor,
        status,
        error,
    }] = rows.as_slice()
    else {
        panic!("the transcript shows the restore start and finish: {rows:?}");
    };
    assert_eq!(
        *started,
        tidebreak_core::CheckpointRestoreStatus::Started,
        "the restore is journaled before any file moves"
    );
    assert_eq!(*status, tidebreak_core::CheckpointRestoreStatus::Completed);
    assert_eq!(*error, None);
    assert_eq!(journaled_id.to_string(), restore_id);
    assert_eq!(
        *target,
        tidebreak_core::CheckpointRestoreTarget::BeforeTurn {
            turn_id: json_id(&second).parse().unwrap()
        }
    );
    assert_eq!(diffstat.files, 3);
    assert_eq!(*actor, None, "the owner restored it");

    // The next turn diffs from the restored state, not from turn 2, and the
    // engine hears which files moved under it.
    std::fs::write(worktree.join("plan.md"), "step one\nstep 2b\n").unwrap();
    let third = run_turn(&client, addr, &token, &session, "try again").await;
    assert_eq!(third["diffstat"]["files"], 1, "{third}");
    let told = adapter.turn_inputs().last().unwrap().text.clone();
    assert!(
        told.contains("restored this workspace's files to how they were before your turn 2")
            && told.contains("- module.rs (gone)")
            && told.ends_with("try again"),
        "{told}"
    );

    // Undo: the state from just before the restore comes back whole, and the
    // turn made since then is what the confirmation lists.
    let response = preview(
        &client,
        addr,
        &token,
        &workspace_id,
        ("restore", &restore_id),
    )
    .await;
    let undo_preview: serde_json::Value = response.json().await.unwrap();
    let response = post(
        &client,
        addr,
        &token,
        &format!("/code/workspaces/{workspace_id}/checkpoints/restore"),
        serde_json::json!({
            "target": { "kind": "before_restore", "restore_id": restore_id },
            "expected_tree": undo_preview["current_tree"],
        }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        read(worktree.join("plan.md")).as_deref(),
        Some("step one\nstep two\n")
    );
    assert_eq!(
        read(worktree.join("by-hand.txt")).as_deref(),
        Some("typed by a person\n")
    );
    assert!(worktree.join("module.rs").exists());
}

/// A restore the process never finished, the way a crash leaves it. The next
/// boot marks its row as stopped partway, keeping its Undo; points the next
/// turn's diff at the files as they really stand, so the engine hears which
/// files moved; and clears the staging folder of the crash's temporary file.
/// The Undo then brings back the state before exactly.
#[tokio::test]
async fn a_restore_the_process_never_finished_is_marked_at_the_next_boot() {
    let adapter = ScriptedAdapter::new(plain_text_script());
    let (router, token, runtime, dir) = code_app_with(adapter.clone()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace).to_owned();
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let session = create_session(&client, addr, &token, &workspace, "plan").await;
    std::fs::write(worktree.join("a.txt"), "a before\n").unwrap();
    std::fs::write(worktree.join("b.txt"), "b before\n").unwrap();
    run_turn(&client, addr, &token, &session, "plan it").await;
    std::fs::write(worktree.join("a.txt"), "a after\n").unwrap();
    std::fs::write(worktree.join("b.txt"), "b after\n").unwrap();
    let second = run_turn(&client, addr, &token, &session, "build it").await;

    // Killed after the first of the two files moved.
    let restore_id = runtime
        .restore_checkpoint_killed_at(
            &OwnerId::local(),
            workspace_id.parse().unwrap(),
            tidebreak_core::CheckpointRestoreTarget::BeforeTurn {
                turn_id: json_id(&second).parse().unwrap(),
            },
            1,
        )
        .await
        .unwrap();
    assert_eq!(read(worktree.join("a.txt")).as_deref(), Some("a before\n"));
    assert_eq!(read(worktree.join("b.txt")).as_deref(), Some("b after\n"));
    let staging = std::path::PathBuf::from(
        String::from_utf8(
            std::process::Command::new("git")
                .args([
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "tidebreak-tmp",
                ])
                .current_dir(&worktree)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim(),
    );
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("0123456789abcdef"), "half written\n").unwrap();

    runtime.recover().await.unwrap();

    let session_id: SessionId = session.parse().unwrap();
    let rows: Vec<Event> = journaled_events(&runtime.db, session_id)
        .await
        .into_iter()
        .map(|framed| framed.event)
        .filter(|event| {
            matches!(event, Event::CheckpointRestored { restore_id: id, .. } if *id == restore_id)
        })
        .collect();
    let [Event::CheckpointRestored {
        status: started, ..
    }, Event::CheckpointRestored { status, error, .. }] = rows.as_slice()
    else {
        panic!("the boot journals how the restore ended: {rows:?}");
    };
    assert_eq!(*started, tidebreak_core::CheckpointRestoreStatus::Started);
    assert_eq!(*status, tidebreak_core::CheckpointRestoreStatus::Partial);
    assert!(
        error.as_deref().is_some_and(|error| error.contains("quit")),
        "{error:?}"
    );
    assert!(!staging.join("0123456789abcdef").exists());

    // The next turn starts from the files as they stand, so it is credited
    // with nothing, and the engine hears the one file that moved.
    let third = run_turn(&client, addr, &token, &session, "carry on").await;
    assert_eq!(third["diffstat"]["files"], 0, "{third}");
    let told = adapter.turn_inputs().last().unwrap().text.clone();
    assert!(
        told.contains("- a.txt") && !told.contains("b.txt") && told.ends_with("carry on"),
        "{told}"
    );

    let response = post(
        &client,
        addr,
        &token,
        &format!("/code/workspaces/{workspace_id}/checkpoints/restore"),
        serde_json::json!({
            "target": { "kind": "before_restore", "restore_id": restore_id.to_string() },
        }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(read(worktree.join("a.txt")).as_deref(), Some("a after\n"));
    assert_eq!(read(worktree.join("b.txt")).as_deref(), Some("b after\n"));
}

#[tokio::test]
async fn a_restore_the_worktree_outgrew_is_refused_and_changes_nothing() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace).to_owned();
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let session = create_session(&client, addr, &token, &workspace, "plan").await;
    std::fs::write(worktree.join("plan.md"), "the turn wrote this\n").unwrap();
    let turn = run_turn(&client, addr, &token, &session, "plan it").await;

    let previewed: serde_json::Value = preview(
        &client,
        addr,
        &token,
        &workspace_id,
        ("turn", json_id(&turn)),
    )
    .await
    .json()
    .await
    .unwrap();
    std::fs::write(worktree.join("later.txt"), "written after the review\n").unwrap();

    let response = post(
        &client,
        addr,
        &token,
        &format!("/code/workspaces/{workspace_id}/checkpoints/restore"),
        serde_json::json!({
            "target": { "kind": "before_turn", "turn_id": json_id(&turn) },
            "expected_tree": previewed["current_tree"],
        }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["kind"], "worktree_changed");
    assert!(worktree.join("plan.md").exists());
    assert!(worktree.join("later.txt").exists());

    let response = preview(&client, addr, &token, &workspace_id, ("turn", "nope")).await;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// Restore, revert, discard, and commit all touch the checkout a running
/// turn is changing, so each one is refused at once until the turn ends, and
/// none of them touches a file.
#[tokio::test]
async fn undo_is_refused_while_a_turn_runs() {
    let adapter = ScriptedAdapter::new(approval_script())
        .with_approvals(CapLevel::Supported)
        .with_delay(Duration::from_millis(20));
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace).to_owned();
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let session = create_session(&client, addr, &token, &workspace, "ask").await;
    std::fs::write(worktree.join("README.md"), "the running turn's edit\n").unwrap();

    let running = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session = session.clone();
        async move {
            client
                .post(format!("http://{addr}/sessions/{session}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "write it" }))
                .send()
                .await
                .unwrap()
        }
    });
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = client
                .get(format!("http://{addr}/approvals?state=pending"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json::<Vec<serde_json::Value>>()
                .await
                .unwrap();
            if let Some(row) = listed.into_iter().next() {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the turn parks on its approval");
    let turn_id = approval["turn_id"].as_str().unwrap().to_owned();

    for (path, body) in [
        (
            format!("/code/workspaces/{workspace_id}/checkpoints/restore"),
            serde_json::json!({ "target": { "kind": "before_turn", "turn_id": turn_id } }),
        ),
        (
            format!("/code/workspaces/{workspace_id}/revert"),
            serde_json::json!({ "path": "README.md" }),
        ),
        (
            format!("/code/workspaces/{workspace_id}/discard"),
            serde_json::json!({ "paths": ["README.md"] }),
        ),
        // A commit that waited for the turn would commit the turn's
        // unreviewed changes under the person's message.
        (
            format!("/code/workspaces/{workspace_id}/git/commit"),
            serde_json::json!({ "message": "what I reviewed" }),
        ),
    ] {
        let response = tokio::time::timeout(
            Duration::from_secs(10),
            post(&client, addr, &token, &path, body),
        )
        .await
        .unwrap_or_else(|_| panic!("{path} waited for the turn instead of refusing"));
        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT, "{path}");
        let refused: serde_json::Value = response.json().await.unwrap();
        assert_eq!(refused["kind"], "turn_running", "{path}: {refused}");
    }
    assert_eq!(
        read(worktree.join("README.md")).as_deref(),
        Some("the running turn's edit\n")
    );

    let decided = post(
        &client,
        addr,
        &token,
        &format!("/approvals/{}/decision", json_id(&approval)),
        serde_json::json!({ "decision": "approve" }),
    )
    .await;
    assert!(decided.status().is_success(), "{}", decided.status());
    let finished = running.await.unwrap();
    assert!(finished.status().is_success());
}

#[tokio::test]
async fn revert_and_discard_change_only_what_the_person_picked() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace).to_owned();
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let lines: String = (1..=30).map(|n| format!("line {n}\n")).collect();
    std::fs::write(worktree.join("README.md"), &lines).unwrap();
    std::fs::write(worktree.join("scratch.txt"), "throwaway\n").unwrap();
    std::fs::write(worktree.join("keep.txt"), "keep me\n").unwrap();

    // The files list says which changes a commit would carry.
    let files: serde_json::Value = client
        .get(format!(
            "http://{addr}/code/workspaces/{workspace_id}/files"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(files["files"]
        .as_array()
        .unwrap()
        .iter()
        .all(|file| file["uncommitted"] == true));

    // Revert one hunk, exactly as the diff view showed it.
    let diff: serde_json::Value = client
        .get(format!("http://{addr}/code/workspaces/{workspace_id}/diff"))
        .bearer_auth(&token)
        .query(&[("file", "README.md")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = diff["diff"].as_str().unwrap();
    let hunk = text[text.find("@@ ").unwrap()..].trim_end_matches('\n');
    let response = post(
        &client,
        addr,
        &token,
        &format!("/code/workspaces/{workspace_id}/revert"),
        serde_json::json!({ "path": "README.md", "hunk": { "index": 0, "text": hunk } }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(read(worktree.join("README.md")).as_deref(), Some("hello\n"));

    // A hunk the view no longer matches is refused.
    let response = post(
        &client,
        addr,
        &token,
        &format!("/code/workspaces/{workspace_id}/revert"),
        serde_json::json!({ "path": "README.md", "hunk": { "index": 0, "text": hunk } }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);

    // Revert a whole file against the base: an added file goes away.
    let response = post(
        &client,
        addr,
        &token,
        &format!("/code/workspaces/{workspace_id}/revert"),
        serde_json::json!({ "path": "keep.txt" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let reverted: serde_json::Value = response.json().await.unwrap();
    assert_eq!(reverted["paths"], serde_json::json!(["keep.txt"]));
    assert!(!worktree.join("keep.txt").exists());

    // Discard drops an uncommitted file and nothing else.
    let response = post(
        &client,
        addr,
        &token,
        &format!("/code/workspaces/{workspace_id}/discard"),
        serde_json::json!({ "paths": ["scratch.txt"] }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(!worktree.join("scratch.txt").exists());
    let response = post(
        &client,
        addr,
        &token,
        &format!("/code/workspaces/{workspace_id}/discard"),
        serde_json::json!({ "paths": ["README.md"] }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["kind"], "no_change");
}

fn head_of(worktree: &std::path::Path) -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(worktree)
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

async fn worktree_tree(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    workspace: &str,
) -> String {
    let files: serde_json::Value = client
        .get(format!("http://{addr}/code/workspaces/{workspace}/files"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    files["worktree_tree"]
        .as_str()
        .expect("the workspace list names the tree it read")
        .to_owned()
}

/// A commit carries what the person reviewed. A file that appeared after
/// the list was read refuses the commit, and nothing is committed.
#[tokio::test]
async fn a_commit_refuses_changes_the_person_did_not_review() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace).to_owned();
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    std::fs::write(worktree.join("reviewed.txt"), "reviewed\n").unwrap();
    let reviewed = worktree_tree(&client, addr, &token, &workspace_id).await;
    std::fs::write(worktree.join("unreviewed.txt"), "slipped in\n").unwrap();
    let head = head_of(&worktree);

    let response = post(
        &client,
        addr,
        &token,
        &format!("/code/workspaces/{workspace_id}/git/commit"),
        serde_json::json!({ "message": "reviewed work", "expected_tree": reviewed }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let refused: serde_json::Value = response.json().await.unwrap();
    assert_eq!(refused["kind"], "worktree_changed", "{refused}");
    assert_eq!(head_of(&worktree), head, "nothing was committed");

    // Committing what the list shows now works.
    let reviewed = worktree_tree(&client, addr, &token, &workspace_id).await;
    let response = post(
        &client,
        addr,
        &token,
        &format!("/code/workspaces/{workspace_id}/git/commit"),
        serde_json::json!({ "message": "reviewed work", "expected_tree": reviewed }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_ne!(head_of(&worktree), head);
}
