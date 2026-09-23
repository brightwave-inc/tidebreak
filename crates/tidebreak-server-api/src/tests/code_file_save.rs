//! Saving a text file from the file viewer: `PUT /code/workspaces/{id}/file`.

use super::code::*;

use crate::code::bus::CodeLiveUpdate;
use crate::code::worktree::content_hash;
use crate::scripted_harness::plain_text_script;
use tidebreak_core::{CodeWorkspace, OwnerId, WorkspaceId};

/// Save `content` over `path`, naming `base_hash`, and return the response.
async fn put_file(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    workspace_id: &str,
    path: &str,
    content: &str,
    base_hash: &str,
) -> reqwest::Response {
    client
        .put(format!("http://{addr}/code/workspaces/{workspace_id}/file"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "path": path,
            "content": content,
            "base_hash": base_hash,
        }))
        .send()
        .await
        .unwrap()
}

async fn get_blob(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    workspace_id: &str,
    path: &str,
) -> serde_json::Value {
    client
        .get(format!("http://{addr}/code/workspaces/{workspace_id}/blob"))
        .bearer_auth(token)
        .query(&[("path", path)])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_save_round_trips_and_tells_the_workspace_views_to_refresh() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace);
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    let loaded = get_blob(&client, addr, &token, workspace_id, "README.md").await;
    assert_eq!(loaded["content"], "hello\n");
    let base = loaded["hash"]
        .as_str()
        .expect("a text file carries its hash");
    assert_eq!(base, content_hash(b"hello\n"));

    let mut updates = runtime.bus.subscribe_updates(&OwnerId::local());
    let text = "hello\r\nfrom the editor\r\n";
    let response = put_file(&client, addr, &token, workspace_id, "README.md", text, base).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let saved: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        saved,
        serde_json::json!({ "path": "README.md", "hash": content_hash(text.as_bytes()) })
    );

    assert_eq!(
        std::fs::read(worktree.join("README.md")).unwrap(),
        text.as_bytes(),
        "the text lands byte for byte, line endings included"
    );
    let reread = get_blob(&client, addr, &token, workspace_id, "README.md").await;
    assert_eq!(reread["content"], text);
    assert_eq!(reread["hash"], saved["hash"]);
    let workspace_id: WorkspaceId = workspace_id.parse().unwrap();
    assert_eq!(
        updates.try_recv().unwrap(),
        CodeLiveUpdate::FilesChanged(workspace_id),
        "the save tells the owner's views to re-read the worktree"
    );
}

#[tokio::test]
async fn a_stale_base_answers_file_changed_and_leaves_the_file_alone() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace);
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let loaded = get_blob(&client, addr, &token, workspace_id, "README.md").await;
    let base = loaded["hash"].as_str().unwrap().to_owned();

    // An agent edits the file after the editor loaded it.
    std::fs::write(worktree.join("README.md"), "the agent's version\n").unwrap();

    let response = put_file(
        &client,
        addr,
        &token,
        workspace_id,
        "README.md",
        "my version\n",
        &base,
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["kind"], "file_changed");
    assert_eq!(
        body["current_hash"],
        content_hash(b"the agent's version\n"),
        "the refusal names what is on disk now, so Overwrite can target it"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "the agent's version\n"
    );

    // Saving against the hash on disk now is how Overwrite lands.
    let response = put_file(
        &client,
        addr,
        &token,
        workspace_id,
        "README.md",
        "my version\n",
        body["current_hash"].as_str().unwrap(),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "my version\n"
    );
}

#[tokio::test]
async fn what_the_viewer_never_offers_to_edit_is_refused() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace);
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    std::fs::create_dir(worktree.join("docs")).unwrap();
    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
    std::fs::write(worktree.join("logo.png"), png).unwrap();
    let dot_git = std::fs::read(worktree.join(".git")).unwrap();
    let readme = content_hash(b"hello\n");

    for (path, base, status, kind) in [
        (
            "../outside.md",
            readme.clone(),
            reqwest::StatusCode::BAD_REQUEST,
            "path_refused",
        ),
        (
            ".git",
            content_hash(&dot_git),
            reqwest::StatusCode::BAD_REQUEST,
            "path_refused",
        ),
        (
            "docs",
            readme.clone(),
            reqwest::StatusCode::BAD_REQUEST,
            "not_a_file",
        ),
        (
            "logo.png",
            content_hash(png),
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            "file_not_editable",
        ),
        (
            "missing.md",
            readme.clone(),
            reqwest::StatusCode::NOT_FOUND,
            "not_found",
        ),
    ] {
        let response = put_file(&client, addr, &token, workspace_id, path, "text\n", &base).await;
        assert_eq!(response.status(), status, "{path}");
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["kind"], kind, "{path}: {body}");
    }
    assert_eq!(std::fs::read(worktree.join(".git")).unwrap(), dot_git);
    assert_eq!(std::fs::read(worktree.join("logo.png")).unwrap(), png);
    assert!(!worktree.join("missing.md").exists());
}

#[tokio::test]
async fn text_over_the_viewer_cap_answers_payload_too_large() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id = json_id(&workspace);
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let huge = "x".repeat(crate::code::file_save::MAX_SAVE_BYTES + 1);

    let response = put_file(
        &client,
        addr,
        &token,
        workspace_id,
        "README.md",
        &huge,
        &content_hash(b"hello\n"),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["kind"], "payload_too_large");
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "hello\n"
    );
}

#[tokio::test]
async fn a_sandbox_workspace_refuses_a_save_with_a_clear_message() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id: WorkspaceId = json_id(&workspace).parse().unwrap();
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let mut remote = runtime
        .get_workspace(&OwnerId::local(), workspace_id)
        .await
        .unwrap();
    remote.worktree_path = CodeWorkspace::remote_worktree_marker(remote.id);
    tidebreak_core::db::code::save_workspace(&runtime.db, &remote)
        .await
        .unwrap();

    let response = put_file(
        &client,
        addr,
        &token,
        &workspace_id.to_string(),
        "README.md",
        "changed\n",
        &content_hash(b"hello\n"),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["kind"], "workspace_remote");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("sandbox workspace")),
        "{body}"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "hello\n"
    );
}
